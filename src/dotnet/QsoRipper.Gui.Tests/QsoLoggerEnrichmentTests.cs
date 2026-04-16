using QsoRipper.Domain;
using QsoRipper.Gui.ViewModels;

namespace QsoRipper.Gui.Tests;

public sealed class QsoLoggerEnrichmentTests
{
    [Fact]
    public void EnrichFromLookupPopulatesAllAvailableFields()
    {
        var qso = new QsoRecord { WorkedCallsign = "N7DOE" };
        var record = new CallsignRecord
        {
            FirstName = "Harry",
            LastName = "Wong",
            GridSquare = "CN87",
            Country = "United States",
            DxccEntityId = 291,
            State = "WA",
            CqZone = 3,
            ItuZone = 7,
            County = "King",
            Iota = "NA-065",
            DxccContinent = "NA",
        };

        QsoLoggerViewModel.EnrichFromLookup(qso, record);

        Assert.Equal("Harry Wong", qso.WorkedOperatorName);
        Assert.Equal("CN87", qso.WorkedGrid);
        Assert.Equal("United States", qso.WorkedCountry);
        Assert.Equal(291u, qso.WorkedDxcc);
        Assert.Equal("WA", qso.WorkedState);
        Assert.Equal(3u, qso.WorkedCqZone);
        Assert.Equal(7u, qso.WorkedItuZone);
        Assert.Equal("King", qso.WorkedCounty);
        Assert.Equal("NA-065", qso.WorkedIota);
        Assert.Equal("NA", qso.WorkedContinent);
    }

    [Fact]
    public void EnrichFromLookupWithNullRecordLeavesQsoUnchanged()
    {
        var qso = new QsoRecord { WorkedCallsign = "W1AW" };

        QsoLoggerViewModel.EnrichFromLookup(qso, null);

        Assert.False(qso.HasWorkedOperatorName);
        Assert.False(qso.HasWorkedGrid);
        Assert.False(qso.HasWorkedCountry);
    }

    [Fact]
    public void EnrichFromLookupWithPartialRecordSetsOnlyAvailableFields()
    {
        var qso = new QsoRecord { WorkedCallsign = "VK3ABC" };
        var record = new CallsignRecord
        {
            FirstName = "Jane",
            Country = "Australia",
        };

        QsoLoggerViewModel.EnrichFromLookup(qso, record);

        Assert.Equal("Jane", qso.WorkedOperatorName);
        Assert.Equal("Australia", qso.WorkedCountry);
        Assert.False(qso.HasWorkedGrid);
        Assert.False(qso.HasWorkedState);
        Assert.Equal(0u, qso.WorkedDxcc);
    }
}
